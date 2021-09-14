package scheduler

import (
	"context"
	"flag"
	"fmt"
	"log"
	"path/filepath"

	"github.com/spf13/viper"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/database"

	batchv1 "k8s.io/api/batch/v1"
	"k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	restclient "k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"
	"k8s.io/client-go/util/homedir"
)

//SimpleScheduler A simple scheduler to run bakta jobs on a Kubernetes cluster
//The config will be picked up from the
type SimpleScheduler struct {
	k8sClient       kubernetes.Interface
	databaseHandler *database.Handler
	namespace       string
}

// InitSimpleScheduler Initiates a scheduler to run bakta jobs
// Can run inside a cluster or outside and will pick up the required Kubernetes configuration
// automatically
func InitSimpleScheduler(dbHandler *database.Handler, clientset kubernetes.Interface) (*SimpleScheduler, error) {

	namespace := viper.GetString("K8sNamespace")

	scheduler := SimpleScheduler{
		k8sClient:       clientset,
		databaseHandler: dbHandler,
		namespace:       namespace,
	}

	return &scheduler, nil
}

func CreateClientSet() (*kubernetes.Clientset, error) {
	var config *restclient.Config
	var err error

	if viper.GetBool("InCluster") {
		config, err = restclient.InClusterConfig()
	} else {
		config, err = createOutOfClusterConfig()
	}
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	clientset, err := kubernetes.NewForConfig(config)
	return clientset, err
}

//StartJob Starts a pre-configurated bakta job on Kubernetes and returns the started job configuration
func (scheduler *SimpleScheduler) StartJob(jobID string, jobConfig *api.JobConfig) (*batchv1.Job, error) {
	job, err := scheduler.databaseHandler.GetJob(jobID)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	downloadConf, err := createDownloadConf(job, jobConfig.HasProdigal, jobConfig.HasReplicons)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	baktaConf, err := createBaktaConf(job, jobConfig)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	uploadConf, err := createUploadConf(job)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	apiJob, err := createBaseJobConf(jobID, scheduler.namespace, downloadConf, baktaConf, uploadConf, job.Secret)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	scheduledJob, err := scheduler.k8sClient.BatchV1().Jobs(scheduler.namespace).Create(context.TODO(), apiJob, metav1.CreateOptions{})
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	err = scheduler.databaseHandler.UpdateK8s(jobID, string(scheduledJob.UID))
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return scheduledJob, nil
}

// Deletes a given bakta job
func (scheduler *SimpleScheduler) DeleteJob(jobName string) error {
	k8sJobName := fmt.Sprintf("%s%s", "bakta-job-", jobName)

	delProp := metav1.DeletePropagationForeground

	err := scheduler.k8sClient.BatchV1().Jobs(scheduler.namespace).Delete(context.TODO(), k8sJobName, metav1.DeleteOptions{
		PropagationPolicy: &delProp,
	})
	if err != nil && !errors.IsNotFound(err) {
		log.Println(err.Error())
		return err
	}

	if errors.IsNotFound(err) {
		log.Println(err.Error())
	}

	return nil
}

//createOutOfClusterConfig tries to create the required k8s configuration from well known config paths
func createOutOfClusterConfig() (*restclient.Config, error) {
	var kubeconfig *string
	if home := homedir.HomeDir(); home != "" {
		kubeconfig = flag.String("kubeconfig", filepath.Join(home, ".kube", "config"), "(optional) absolute path to the kubeconfig file")
	} else {
		kubeconfig = flag.String("kubeconfig", "", "absolute path to the kubeconfig file")
	}
	flag.Parse()

	// use the current context in kubeconfig
	config, err := clientcmd.BuildConfigFromFlags("", *kubeconfig)
	if err != nil {
		panic(err.Error())
	}

	return config, err
}

func (scheduler *SimpleScheduler) GetK8sClient() kubernetes.Interface {
	return scheduler.k8sClient
}

func (scheduler *SimpleScheduler) GetNamespace() string {
	return scheduler.namespace
}
