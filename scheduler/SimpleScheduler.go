package scheduler

import (
	"context"
	"flag"
	"log"
	"os"
	"path/filepath"

	"github.com/spf13/viper"

	"github.com/ag-computational-bio/bakta-web-api-go/api"
	"github.com/ag-computational-bio/bakta-web-backend/database"

	restclient "k8s.io/client-go/rest"

	batchv1 "k8s.io/api/batch/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/tools/clientcmd"
	"k8s.io/client-go/util/homedir"
)

//SimpleScheduler A simple scheduler to run bakta jobs on a Kubernetes cluster
//The config will be picked up from the
type SimpleScheduler struct {
	k8sClient       *kubernetes.Clientset
	databaseHandler *database.Handler
	namespace       string
}

//InitSimpleScheduler Initiates a scheduler to run bakta jobs
func InitSimpleScheduler(dbHandler *database.Handler) (*SimpleScheduler, error) {
	var config *restclient.Config
	var err error

	if os.Getenv("InCluster") != "" {
		config, err = restclient.InClusterConfig()
	} else {
		config, err = createOutOfClusterConfig()
	}

	clientset, err := kubernetes.NewForConfig(config)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	namespace := viper.GetString("K8sNamespace")

	scheduler := SimpleScheduler{
		k8sClient:       clientset,
		databaseHandler: dbHandler,
		namespace:       namespace,
	}

	return &scheduler, nil
}

//StartJob Starts a bakta job on Kubernetes
func (scheduler *SimpleScheduler) StartJob(jobID string, jobConfig *api.JobConfig) (*batchv1.Job, error) {
	job, err := scheduler.databaseHandler.GetJob(jobID)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	downloadConf, err := createDownloadConf(job, false, false)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	baktaConf, err := createBaktaConf(job, jobConfig, job.ConfString)
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

	err = scheduler.databaseHandler.UpdateK8s(jobID, string(scheduledJob.UID), job.ConfString)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return scheduledJob, nil
}

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

func (scheduler *SimpleScheduler) GetK8sClient() *kubernetes.Clientset {
	return scheduler.k8sClient
}

func (scheduler *SimpleScheduler) GetNamespace() string {
	return scheduler.namespace
}
