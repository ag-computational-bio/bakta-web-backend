package scheduler

import (
	"fmt"
	"log"

	"github.com/spf13/viper"
	"k8s.io/apimachinery/pkg/api/resource"

	batchv1 "k8s.io/api/batch/v1"
	v1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

// createBaseJobConf
// Create the configured bakta kubernetes job from the configration
func createBaseJobConf(
	id string,
	namespace string,
	downloaderConf string,
	baktaConf string,
	uploaderConf string,
	secret string) (*batchv1.Job, error) {

	updateServiceName := viper.GetString("UpdateService.Name")
	if updateServiceName == "" {
		err := fmt.Errorf("could not find service under config UpdateService.Name")
		log.Println(err)
		return nil, err
	}

	updateServicePort := viper.GetString("UpdateService.Port")
	if updateServicePort == "" {
		err := fmt.Errorf("could not find service under config UpdateService.Port")
		log.Println(err)
		return nil, err
	}

	cpuQuantityStringLimit := viper.GetString("Job.CPU.Limit")
	if cpuQuantityStringLimit == "" {
		cpuQuantityStringLimit = "4"
	}

	memoryQuantityStringLimit := viper.GetString("Job.Memory.Limit")
	if memoryQuantityStringLimit == "" {
		memoryQuantityStringLimit = "4000Mi"
	}

	cpuQuantityStringRequest := viper.GetString("Job.CPU.Request")
	if cpuQuantityStringRequest == "" {
		cpuQuantityStringRequest = "4"
	}

	memoryQuantityStringRequest := viper.GetString("Job.Memory.Request")
	if memoryQuantityStringRequest == "" {
		memoryQuantityStringRequest = "4000Mi"
	}

	cpuQuantityLimit, err := resource.ParseQuantity(cpuQuantityStringLimit)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	memoryQuantityLimit, err := resource.ParseQuantity(memoryQuantityStringLimit)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	cpuQuantityRequest, err := resource.ParseQuantity(cpuQuantityStringRequest)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	memoryQuantityRequest, err := resource.ParseQuantity(memoryQuantityStringRequest)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	claimName := viper.GetString("BaktaDatabasePVCName")
	if claimName == "" {
		log.Println("could not find pvc name for the bakta database")
		return nil, fmt.Errorf("could not find pvc name for the bakta database")
	}

	resourceRequests := make(map[v1.ResourceName]resource.Quantity)
	resourceRequests[v1.ResourceCPU] = cpuQuantityRequest
	resourceRequests[v1.ResourceMemory] = memoryQuantityRequest

	resourceLimit := make(map[v1.ResourceName]resource.Quantity)
	resourceLimit[v1.ResourceCPU] = cpuQuantityLimit
	resourceLimit[v1.ResourceMemory] = memoryQuantityLimit

	//Required to convert const to int32 ref
	job_image := viper.GetString("JobContainer")

	job := &batchv1.Job{
		ObjectMeta: metav1.ObjectMeta{
			Name:      fmt.Sprintf("bakta-job-%v", id),
			Namespace: namespace,
		},
		Spec: batchv1.JobSpec{
			Template: v1.PodTemplateSpec{
				Spec: v1.PodSpec{
					RestartPolicy: "Never",
					Containers: []v1.Container{
						{
							Name:  "bakta-job",
							Image: job_image,
							Lifecycle: &v1.Lifecycle{
								PostStart: &v1.Handler{
									Exec: &v1.ExecAction{
										Command: []string{
											"/bin/bash",
											"-c",
											"/bin/DataStager update",
										},
									},
								},
							},
							Resources: v1.ResourceRequirements{
								Limits:   resourceLimit,
								Requests: resourceRequests,
							},
							VolumeMounts: []v1.VolumeMount{
								{
									Name:      "database",
									MountPath: "/db",
								},
								{
									Name:      "cache-volume",
									MountPath: "/cache",
								},
							},
							Env: []v1.EnvVar{
								{
									Name:  "DownloaderEnvConfig",
									Value: downloaderConf,
								},
								{
									Name:  "BaktaEnvConfig",
									Value: baktaConf,
								},
								{
									Name:  "UploaderEnvConfig",
									Value: uploaderConf,
								},
								{
									Name:  "JobID",
									Value: id,
								},
								{
									Name:  "GRPCUpdaterEndpoint",
									Value: updateServiceName,
								},
								{
									Name:  "GRPCUpdaterPort",
									Value: updateServicePort,
								},
								{
									Name: "AWS_ACCESS_KEY_ID",
									ValueFrom: &v1.EnvVarSource{
										SecretKeyRef: &v1.SecretKeySelector{
											Key: "AccessKey",
											LocalObjectReference: v1.LocalObjectReference{
												Name: "s3",
											},
										},
									},
								},
								{
									Name: "AWS_SECRET_ACCESS_KEY",
									ValueFrom: &v1.EnvVarSource{
										SecretKeyRef: &v1.SecretKeySelector{
											Key: "SecretKey",
											LocalObjectReference: v1.LocalObjectReference{
												Name: "s3",
											},
										},
									},
								},
							},
						},
					},

					Volumes: []v1.Volume{
						{
							Name: "cache-volume",
							VolumeSource: v1.VolumeSource{
								EmptyDir: &v1.EmptyDirVolumeSource{},
							},
						},
						{
							Name: "database",
							VolumeSource: v1.VolumeSource{
								PersistentVolumeClaim: &v1.PersistentVolumeClaimVolumeSource{
									ClaimName: claimName,
								},
							},
						},
					},
				},
			},
			BackoffLimit: int32Link(1),
		},
	}

	return job, nil
}

func int32Link(value int) *int32 {
	int32Value := int32(value)
	return &int32Value
}
