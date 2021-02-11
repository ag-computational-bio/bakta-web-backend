package scheduler

import (
	"fmt"
	"log"

	"k8s.io/apimachinery/pkg/api/resource"

	batchv1 "k8s.io/api/batch/v1"
	v1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

func createBaseJobConf(
	id string,
	namespace string,
	downloaderConf string,
	baktaConf string,
	uploaderConf string,
	secret string) (*batchv1.Job, error) {

	cpuQuantity, err := resource.ParseQuantity("8")
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	memoryQuantity, err := resource.ParseQuantity("8000Mi")
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	resourceRequests := make(map[v1.ResourceName]resource.Quantity)
	resourceRequests[v1.ResourceCPU] = cpuQuantity
	resourceRequests[v1.ResourceMemory] = memoryQuantity

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
							Image: "quay.io/mariusdieckmann/bakta-web-job",
							Lifecycle: &v1.Lifecycle{
								PostStart: &v1.Handler{
									Exec: &v1.ExecAction{
										Command: []string{
											"/bni/bash",
											"-c",
											"/bin/DataStager update",
										},
									},
								},
								PreStop: &v1.Handler{
									Exec: &v1.ExecAction{
										Command: []string{
											"/bni/bash",
											"-c",
											"/bin/DataStager update",
										},
									},
								},
							},
							Resources: v1.ResourceRequirements{
								Limits:   resourceRequests,
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
									ClaimName: "database",
								},
							},
						},
					},
				},
			},
			BackoffLimit: int32Link(4),
		},
	}

	return job, nil
}

func int32Link(value int) *int32 {
	int32Value := int32(value)
	return &int32Value
}
